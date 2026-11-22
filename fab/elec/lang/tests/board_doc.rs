use fab_elec_board::{Anchor, CustomCheck, CustomRule, LabelPose, Origin, RuleCode, RuleSet};
use fab_elec_core::{Angle, Canon, ContentHash, DocId, FeatureRef, LayerId, LayerSet, OnTrack, Sel, Severity, P};
use fab_elec_lang::{board_doc::{self, CodeKind}, source::line_col, write};

#[test]
fn ledctl_board_round_trips() {
    let d = fab_elec_fixtures::ledctl();
    let circuit = &d.units[0].circuit;
    let board = d.units[0].board.as_ref().expect("fixture has a board");
    let text = write(board, circuit);
    let lowered = board_doc::lower(DocId(0), &text, circuit, None).expect("fixture document lowers");
    assert_eq!(lowered.board.content_hash(), board.content_hash(), "fixture content survives the document round trip");
    assert_eq!(write(&lowered.board, circuit), text, "fixture text is a fixed point");
    assert!(!text.contains("body_z:"), "part-derived body bands are omitted");
    assert!(!text.contains("_label:"), "a placement without its own label poses writes no label key");
    assert_eq!(text.matches("place(").count(), board.placed.len(), "every placement has a statement");
    assert_eq!(text.matches("track(").count(), 8, "all eight tracks have statements");
    assert!(text.contains("\"lamps[0].r:2\""), "track ends name their pads");
    assert!(text.lines().filter(|s| s.starts_with("track(")).all(|s| s.contains(".r:2\"") && s.contains(".d:2\"")), "every fixture track keeps both pad references");
    assert_eq!(text.matches("via(").count(), 2, "both vias have statements");
    assert_eq!(text.matches("zone(").count(), 1, "the zone has a statement");
    assert_eq!(text.matches("hole(").count(), 2, "both mounting holes have statements");
    assert!(!text.contains("stackup(") && !text.contains("rules("), "standard stackup and rules are omitted");
    let items = board.placed.iter().map(|p| (p.id, "place("))
        .chain(board.tracks.iter().map(|t| (t.id, "track(")))
        .chain(board.vias.iter().map(|v| (v.id, "via(")))
        .chain(board.zones.iter().map(|z| (z.id, "zone(")))
        .chain(board.holes.iter().map(|h| (h.id, "hole(")));
    for (id, name) in items {
        let span = &lowered.spans.iter().find(|(s, _)| *s == Sel::Item(board.unit, id)).expect("every item has a source span").1;
        assert!(text[span.start as usize..span.end as usize].starts_with(name), "item {} maps to its {name} statement", id.0);
    }
    let unit_span = &lowered.spans.iter().find(|(s, _)| *s == Sel::Unit(board.unit)).expect("board has a source span").1;
    assert_eq!(&text[unit_span.start as usize..unit_span.end as usize], text.lines().next().expect("board line exists"), "unit maps to the complete board line");
    assert!(text[unit_span.start as usize..].starts_with("board("), "unit span begins with board");
    assert!(lowered.spans.iter().any(|(s, _)| *s == Sel::Feature(FeatureRef::Outline(board.unit))), "outline has a feature span");

    // A part's `label` field is its legend: front silk text placed beside it, read from the part
    // and never written into the board's text.
    let mut labelled = (**circuit).clone();
    labelled.comps.iter_mut().find(|c| c.path == "lamps[0].r").expect("the lamp's resistor").fields.0.push(("label".into(), "GAIN".into()));
    let marked = board_doc::lower(DocId(0), &text, &labelled, None).expect("the labelled document lowers").board;
    let placed = marked.placed.iter().find(|p| p.path == "lamps[0].r").expect("the resistor is placed");
    let legend = placed.legend.as_ref().expect("the label gives the part a legend");
    assert_eq!((legend.text.as_str(), legend.layer), ("GAIN", LayerId::F_SILK), "the legend is the label, on the front silk");
    assert_ne!(legend.at, fab_elec_board::text::legend_text("GAIN", &placed.footprint).at, "the board placed the legend");
    assert!(marked.placed.iter().filter(|p| p.path != "lamps[0].r").all(|p| p.legend.is_none()), "a part without a label has no legend");
    assert_ne!(marked.content_hash(), lowered.board.content_hash(), "the legend is the board's content");
    assert_eq!(write(&marked, &labelled), text, "the legend is never written into the board's text");
}

#[test]
fn every_statement_reads_back_and_errors_are_located() {
    let d = fab_elec_fixtures::ledctl();
    let circuit = &d.units[0].circuit;
    let document = r##"board("options \"and\" escapes")
stackup({finish: @enig thickness: 1.8mm mask: #12345678 silk: #fedcbaff layers: [{silk: @top thickness: 0mm material: "ink"}, {paste: @top thickness: 0.01mm material: "paste"}, {mask: @top thickness: 0.02mm material: "mask" er: 3.2}, {copper: "F.Cu" thickness: 0.035mm material: "copper" velocity: 150000000 role: @mixed}, {dielectric: @prepreg thickness: 0.2mm material: "glass" er: 4.2 loss_tan: 0.015}, {copper: "In1.Cu" thickness: 0.035mm material: "copper" role: @plane}, {dielectric: @core thickness: 1.25mm material: "glass"}, {copper: "In2.Cu" thickness: 0.035mm material: "copper" role: @plane}, {dielectric: @prepreg thickness: 0.2mm material: "glass"}, {copper: "B.Cu" thickness: 0.035mm material: "copper"}, {mask: @bottom thickness: 0.02mm material: "mask"}, {paste: @bottom thickness: 0.01mm material: "paste"}, {silk: @bottom thickness: 0mm material: "ink"}]})
outline([[[0mm, 0mm], [30mm, 0mm], arc([31mm, 10mm], [30mm, 20mm]), [0mm, 20mm]]])
rules({clearance: nil track: 0.2mm via: {drill: 0.2mm pad: 0.5mm} drill: 0.25mm annular: 0.1mm edge: 0.4mm hole_to_hole: 0.35mm hole_clearance: 0.3mm pair_gap: 0.25mm})
waive("0123456789abcdef", "reviewed \"spacing\"")
rule("spacing", {when: fn(a, b) {true} layers: ["F.Cu"] set: {clearance: 0.4mm} severity: @error})
check("shape", fn(b) {true}, {severity: @info})
place("lamps[0].r", [1inch, - 2mil], {rot: 22.5deg side: @bottom body_z: [0.5mm, 2mm] ref_label: {at: [0mm, -1.5mm] rot: 90deg pinned: true} value_label: {at: [0.25mm, 1.5mm]} locked: true block: "mcu"})
let host = track("GND", "F.Cu", 0.2mm, [[0mm, 0mm], [10mm, 0mm]], {widths: [0.2mm, 0.4mm] locked: true solved: 7})
let branch = track("GND", "F.Cu", 0.2mm, [[5mm, 5mm], [5mm, 0mm]])
let copper = track("GND", "F.Cu", 0.2mm, [[20mm, 0mm], [22mm, 0mm]], {block: "mcu"})
track("lamps[0].d.A", "B.Cu", 250um, [[20mm, 20mm], arc([21mm, 10mm], "lamps[0].r:2")])
via("GND", [10mm, 0mm], {drill: 0.2mm pad: 0.55mm span: ["F.Cu", "In1.Cu"] kind: @blind tent: @front backdrill: [@top, "In2.Cu"] locked: true solved: 3})
zone("GND", ["B.Cu", "F.Cu"], [{outer: [[1mm, 1mm], [9mm, 1mm], [9mm, 9mm], [1mm, 9mm]] holes: [[[3mm, 3mm], [3mm, 4mm], [4mm, 4mm], [4mm, 3mm]]]}], {name: "ground area" priority: 3 clearance: 0.33mm min_width: 0.21mm connect: @thru_thermal thermal: {gap: 0.4mm spoke: 0.25mm angle: 45deg} islands: @keep locked: true block: "mcu"})
zone(nil, ["In2.Cu"], @board)
keepout([{outer: [[11mm, 1mm], [12mm, 1mm], [12mm, 2mm], [11mm, 2mm]]}], ["F.Cu"], {forbid: [@tracks, @vias] reason: "space" z: [1mm, 3mm] solved: 4})
hole("mount", [22mm, 0mm], 0.8mm, {pad: 1.2mm net: "GND" plated: true block: "mcu"})
cutout("slot", [[15mm, 1mm], [17mm, 1mm], arc([18mm, 2mm], [17mm, 3mm]), [15mm, 3mm]], {solved: 5})
graphic("F.SilkS", stroke([[1mm, 11mm], arc([2mm, 12mm], [3mm, 11mm])], 0.15mm), {locked: true block: "mcu"})
graphic("F.SilkS", fill(circle([4mm, 11mm], 0.5mm)))
graphic("F.SilkS", fill(segment([5mm, 11mm], [6mm, 11mm], 0.25mm)))
graphic("F.SilkS", fill(arc([7mm, 11mm], [8mm, 12mm], [9mm, 11mm], 0.25mm)))
graphic("B.SilkS", fill(rect([10mm, 11mm], [0.5mm, 1mm], -90deg, 0.1mm)))
graphic("B.SilkS", fill(oval([12mm, 11mm], [0.5mm, 1mm], 1.5707963267948966rad)))
graphic("B.SilkS", fill(poly([{outer: [[13mm, 10mm], [14mm, 10mm], [14mm, 12mm], [13mm, 12mm]]}])))
text("line\n\"quoted\" \\ µ", "B.SilkS", [20.123456mm, -0.0125mm], {size: 1mm thick: 0.15mm rot: -22.5deg mirrored: true locked: true solved: 6})
tuning("GND", skew("VBUS", 0.1mm), {pair: "VBUS" host: [host, branch] copper: [copper] side: @left amp: [0.2mm, 0.8mm] pitch: 0.5mm style: @forty_five solved: 7})
tuning("GND", len(1cm), {amp: [0.1mm, 1mm] pitch: 0.5mm side: @right style: @any_angle})
tuning("GND", delay(1250ps), {amp: [0.1mm, 1mm] pitch: 0.5mm})
tuning("GND", group("matched"), {amp: [0.1mm, 1mm] pitch: 0.5mm})
"##;
    let first = board_doc::lower(DocId(1), document, circuit, None).expect("all board statements lower");
    assert_eq!(first.code.iter().map(|c| c.kind).collect::<Vec<_>>(), vec![CodeKind::Rule, CodeKind::Check], "custom functions are deferred in statement order");
    assert_eq!(first.code.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["spacing", "shape"], "deferred statements retain their names");
    for code in &first.code {
        let s = &document[code.span.start as usize..code.span.end as usize];
        assert!(s.starts_with(if code.kind == CodeKind::Rule { "rule(" } else { "check(" }), "deferred span covers its whole statement");
    }
    let board = &first.board;
    assert_eq!(board.stackup.copper().len(), 4, "four copper layers lower");
    assert_eq!(board.rules.base.clearance, None, "nil clears a standard rule");
    assert_eq!(board.rules.base.pair_gap, Some(250_000), "pair_gap reads a length into the base rules");
    assert_eq!(board.placed[0].at.y, -50_800, "a separated unary minus retains the length unit");
    assert_eq!(board.placed[0].at.x, 25_400_000, "inch literals scale to metres");
    assert_eq!(board.placed[0].body_z, Some((500_000, 2_000_000)), "an explicit body band overrides the part's body");
    assert_eq!(board.placed[0].ref_label, Some(LabelPose { at: P::new(0, -1_500_000), rot: Angle::deg(90.0), pinned: true }), "a reference label pose reads its centre, turn and pin");
    assert_eq!(board.placed[0].value_label, Some(LabelPose { at: P::new(250_000, 1_500_000), rot: Angle::ZERO, pinned: false }), "a value label pose reads with no turn and unpinned by default");
    assert_eq!(board.keepouts[0].z, Some((1_000_000, 3_000_000)), "a keepout retains its height band");
    assert_eq!(board.tracks[0].ends[1], Anchor::Via(board.vias[0].id), "a coincident via wins anchor inference");
    assert_eq!(board.tracks[1].ends[1], Anchor::Track { id: board.tracks[0].id, at: OnTrack { seg: 0, frac: OnTrack::ONE / 2 } }, "an exact segment intersection anchors to its track");
    assert_eq!(board.tracks[2].ends[1], Anchor::Hole(board.holes[0].id), "a coincident hole becomes an anchor");
    assert!(matches!(board.tracks[3].ends[1], Anchor::Pad { .. }), "an arc end retains its pad anchor");
    assert_eq!(board.zones[1].net, None, "a nil zone has no net");
    let mcu = circuit.tree.iter().find(|b| b.path == "mcu").expect("fixture has an mcu block").id;
    assert_eq!(board.placed[0].origin, Origin::Block { block: mcu, detached: true }, "a block is named by its instance path");
    let bound = &first.spans.iter().find(|(s, _)| *s == Sel::Item(board.unit, board.tracks[0].id)).expect("bound track has a span").1;
    assert!(document[bound.start as usize..bound.end as usize].starts_with("let host = track("), "a bound track span includes let");
    let text = write(board, circuit);
    let second = board_doc::lower(DocId(2), &text, circuit, None).expect("canonical options document lowers");
    assert_eq!(second.board.content_hash(), board.content_hash(), "all literal board content survives the round trip");
    assert_eq!(write(&second.board, circuit), text, "all options and polygon normalisation reach a text fixed point");
    assert!(text.contains("body_z: [0.5mm, 2mm]"), "an overridden body band is written with length units");
    assert!(text.contains("ref_label: {at: [0mm, -1.5mm] rot: 90deg pinned: true} value_label: {at: [0.25mm, 1.5mm]}"), "label poses are written with only their set fields");
    assert!(text.lines().any(|s| s.starts_with("keepout(") && s.contains("z: [1mm, 3mm]")), "a keepout band is written with length units");
    assert!(text.contains("let t0 = track(") && text.contains("host: [t0, t1] copper: [t2]"), "tunings use canonical track bindings");
    assert!(text.contains("20.123456mm") && text.contains("-0.0125mm"), "integer lengths keep all six decimal places");
    assert!(text.contains("delay(1.25ns)"), "delay targets use nanoseconds");
    assert!(text.contains("block: \"mcu\"") && !text.contains("block: 2"), "block provenance is written as a path, never an id");
    assert_eq!(second.board.zones[0].area, board.zones[0].area, "polygon holes survive normalisation exactly");

    let mut with_code = board.clone();
    let code = RuleCode { id: 0, hash: ContentHash([1; 16]), at: (1, 0) };
    with_code.rules.custom.push(CustomRule {
        name: "spacing".into(), when: code.clone(), layers: Some(LayerSet::one(LayerId::F_CU)),
        set: RuleSet { clearance: Some(400_000), ..RuleSet::default() }, severity: Severity::Error,
    });
    with_code.rules.checks.push(CustomCheck { name: "shape".into(), code, severity: Severity::default() });
    let nil_code = write(&with_code, circuit);
    assert!(nil_code.contains("when: nil") && nil_code.contains("check(\"shape\", nil)"), "missing code writes nil and default severity is omitted");
    with_code.rules.checks[0].severity = Severity::Info;
    let supplied = board_doc::write_with(&with_code, circuit, &|_| Some("fn(a, b) {true}".into()));
    assert!(supplied.contains("severity: @error") && supplied.contains("severity: @info"), "non-default rule and check severities are written");
    let deferred = board_doc::lower(DocId(3), &supplied, circuit, None).expect("supplied functions parse without evaluation");
    assert_eq!(deferred.code.len(), 2, "supplied functions are both deferred");
    assert_eq!(deferred.board.content_hash(), board.content_hash(), "deferred functions leave literal board content intact");

    let bad = concat!(
        "place(\"mcu.x\", [0mm, 0mm])\n",
        "track(\"VBUS\", \"F.Cu\", 0.2mm, [\"U9:1\", [1mm, 0mm]])\n",
        "via(\"GND\", [0mm, 0mm], {drill: 0.3mm pad: 0.6mm dril: 0.2mm})\n",
        "zone(\"GND\", [\"F.Cuu\"], @board)\n",
        "hole(\"m1\", [0mm, 0mm], 3)\n",
        "cutout(\"c\", [[0mm, 0mm], [1mm, 0mm], [1mm, 1mm]], {block: \"lamps[9]\"})\n",
        "place(\"lamps[0].r\", [0mm, 0mm], {body_z: [0mm]})\n",
        "place(\"lamps[1].r\", [0mm, 0mm], {ref_label: {at: [0mm, 0mm] pin: true}})\n",
    );
    let errors = board_doc::lower(DocId(4), bad, circuit, None).expect_err("eight independent mistakes are rejected");
    assert_eq!(errors.len(), 8, "all eight mistakes are collected");
    for (i, (error, (name, column, message))) in errors.iter().zip([
        ("place", 7, "no component has the path mcu.x"),
        ("track", 31, "U9 is not placed"),
        ("via", 49, "unknown option field dril"),
        ("zone", 14, "is not a layer"),
        ("hole", 24, "drill must be a length"),
        ("cutout", 59, "no block has the path \"lamps[9]\""),
        ("place", 42, "body_z must have two entries"),
        ("place", 61, "unknown option field pin"),
    ]).enumerate() {
        assert!(error.message.starts_with(&format!("{name}(\"")) && error.message.contains(message), "diagnostic identifies {name} and its failed property: {}", error.message);
        assert_eq!(error.sel.len(), 1, "diagnostic has one source location");
        let Sel::Source { doc, start, end } = error.sel[0] else { panic!("diagnostic must select source text"); };
        assert_eq!(doc, DocId(4), "diagnostic names the input document");
        assert_eq!(line_col(bad, start), Some((i as u32 + 1, column)), "{name} diagnostic points at its offending node");
        assert!(end > start && (end as usize) <= bad.len(), "{name} diagnostic span is nonempty and in bounds");
    }
    let malformed = "mystery(\"x\")\nhole(\"syntax\", [0mm, 0mm], 0.3mm\n";
    let errors = board_doc::lower(DocId(5), malformed, circuit, None).expect_err("syntax and statement errors are rejected together");
    assert_eq!(errors.len(), 2, "a syntax error does not hide another statement's error");
    assert!(errors[0].message.contains("unknown statement mystery"), "unknown calls name the statement");
    assert!(errors[1].message.contains("syntax:"), "syntax errors retain their parser explanation");
}

#[test]
fn a_written_waiver_matches_after_the_document_renumbers() {
    use fab_elec_board::{connect, drc, resolve_rules, Board, Waiver};
    use fab_elec_circuit::CheckKind;
    let d = fab_elec_fixtures::ledctl();
    let circuit = &d.units[0].circuit;
    let run = |b: &Board| drc(b, &connect(b, &[]), &resolve_rules(b, None), &[], None);
    let document = concat!(
        "board(\"w\")\n",
        "outline([[[0mm, 0mm], [20mm, 0mm], [20mm, 20mm], [0mm, 20mm]]])\n",
        "track(\"GND\", \"F.Cu\", 0.2mm, [[3mm, 5mm], [7mm, 5mm]])\n",
        "track(\"VBUS\", \"F.Cu\", 0.2mm, [[3mm, 5.3mm], [7mm, 5.3mm]])\n",
        "via(\"GND\", [12mm, 12mm], {drill: 0.3mm pad: 0.6mm})\n",
    );
    let mut first = board_doc::lower(DocId(0), document, circuit, None).expect("the waiver board lowers").board;
    let found = run(&first).violations.into_iter().find(|v| v.kind == CheckKind::Clearance)
        .expect("the two tracks violate clearance");
    first.rules.waivers.push(Waiver { violation: found.id, reason: "accepted spacing".into() });
    let text = write(&first, circuit);
    assert!(text.contains(&format!("waive(\"{:016x}\", \"accepted spacing\")", found.id)), "the waiver is written by its content id");
    // The via's statement moves first: every id is issued again in the new statement order.
    let via = text.lines().find(|l| l.starts_with("via(")).expect("the via is written");
    let header_end = text.find("track(").expect("tracks are written");
    let renumbered = format!("{}{via}\n{}", &text[..header_end], text[header_end..].replace(&format!("{via}\n"), ""));
    let second = board_doc::lower(DocId(1), &renumbered, circuit, None).expect("the reordered document lowers").board;
    assert_ne!(second.tracks[0].id, first.tracks[0].id, "the reordered document really renumbers the tracks");
    assert_eq!(second.rules.waivers, first.rules.waivers, "the waiver reads back unchanged");
    assert!(!run(&second).violations.iter().any(|v| v.kind == CheckKind::Clearance), "the waiver still matches its finding after renumbering");
}
